//! Virtual uinput tablet device: a pen/stylus input device libinput (and
//! therefore GIMP/Krita/every other app) recognizes as a real graphics
//! tablet -- pressure, tilt, hover-proximity, and the S Pen side button.
//! Finger touches (Milestone 6 follow-up) are reported via this exact same
//! `BTN_TOOL_PEN` proximity path rather than `BTN_TOOL_FINGER`: an earlier
//! attempt at a real `BTN_TOOL_FINGER` distinction wrote successfully (no
//! I/O errors) but never moved the cursor -- `BTN_TOOL_FINGER` is also the
//! standard capability bit for touchpads, and it's plausible libinput
//! reclassified this device into touchpad (relative-motion) semantics for
//! finger-tagged events instead of tablet (absolute-positioning) ones.
//! `BTN_TOOL_PEN` is the confirmed-working path, so finger touches reuse it.
//!
//! Two more things confirmed live and load-bearing, not obvious from the
//! kernel docs: `ABS_PRESSURE` has to be sent on every event (including
//! finger touches, where it isn't semantically meaningful) or the cursor
//! doesn't move at all -- this device's tablet-tool motion handling
//! apparently gates on nonzero pressure, not just `BTN_TOUCH` -- and it has
//! to be forced to 0 whenever not in contact, or a stale nonzero value left
//! over from the last touch keeps the pointer stuck "down" after release.
//!
//! Milestone 5 scope: the device itself and synthetic-event injection.
//! Wiring real Android MotionEvent data into this is Milestone 6.

use input_linux::{
    AbsoluteAxis, AbsoluteInfo, AbsoluteInfoSetup, EventKind, EventTime, InputEvent, InputId,
    Key, SynchronizeKind, UInputHandle,
};
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io;

pub struct UinputTablet {
    handle: UInputHandle<File>,
    // Real hardware only emits a key/button event when its value actually
    // changes; libinput's tablet-tool state machine expects that
    // edge-triggered behavior (proximity-in once, then contact down/up on
    // real transitions), not a constant re-assertion every frame.
    tool_in_proximity: Cell<bool>,
    in_contact: Cell<bool>,
    // S Pen side button (BTN_STYLUS): decoupled from position updates --
    // Android reports it via its own ACTION_BUTTON_PRESS/RELEASE stream,
    // independent of whether the pen is currently hovering or in contact.
    button_pressed: Cell<bool>,
}

/// Axis ranges as reported by the capability handshake (Android's
/// `Display` metrics / `InputDevice.getMotionRange()`) -- never hardcoded
/// per-device constants, per the design doc's hard constraint. Milestone 5
/// picks concrete numbers itself since there's no real Android connection
/// yet; Milestone 6 wires these from the real handshake.
pub struct TabletRanges {
    pub width: i32,
    pub height: i32,
    pub pressure_max: i32,
    pub tilt_min: i32,
    pub tilt_max: i32,
}

impl UinputTablet {
    pub fn create(ranges: &TabletRanges) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")?;
        let handle = UInputHandle::new(file);

        handle.set_evbit(EventKind::Key)?;
        handle.set_keybit(Key::ButtonToolPen)?;
        handle.set_keybit(Key::ButtonTouch)?;
        handle.set_keybit(Key::ButtonStylus)?;

        handle.set_evbit(EventKind::Absolute)?;
        handle.set_absbit(AbsoluteAxis::X)?;
        handle.set_absbit(AbsoluteAxis::Y)?;
        handle.set_absbit(AbsoluteAxis::Pressure)?;
        handle.set_absbit(AbsoluteAxis::TiltX)?;
        handle.set_absbit(AbsoluteAxis::TiltY)?;

        let abs = [
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::X,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: ranges.width,
                    fuzz: 0,
                    flat: 0,
                    // units/mm -- libinput needs a real (non-zero) value here
                    // to compute a valid tablet-to-screen mapping; a real
                    // uinput tablet driver (rmTabletDriver, for the
                    // reMarkable) sets ~100 here. 0 (what we had) meant
                    // "no calibration data", which is very plausibly why
                    // motion never showed up on screen despite the device
                    // being correctly recognized as a tablet by udev.
                    resolution: 100,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::Y,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: ranges.height,
                    fuzz: 0,
                    flat: 0,
                    resolution: 100,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::Pressure,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: ranges.pressure_max,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::TiltX,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: ranges.tilt_min,
                    maximum: ranges.tilt_max,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::TiltY,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: ranges.tilt_min,
                    maximum: ranges.tilt_max,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
        ];

        let id = InputId {
            bustype: input_linux::sys::BUS_VIRTUAL as u16,
            vendor: 0x1209,  // pid.codes test/prototype vendor ID
            product: 0x0001,
            version: 1,
        };
        handle.create(&id, b"Quill Virtual Tablet", 0, &abs)?;

        Ok(Self {
            handle,
            tool_in_proximity: Cell::new(false),
            in_contact: Cell::new(false),
            button_pressed: Cell::new(false),
        })
    }

    /// One full pen/finger-position update: move to (x, y), set contact
    /// (BTN_TOUCH + BTN_TOOL_PEN), then SYN_REPORT so the kernel delivers it
    /// as one atomic input frame. Key/button events are only emitted on an
    /// actual state transition (see the `tool_in_proximity`/`in_contact` doc
    /// comment above) -- calling this at all implies the tool is in
    /// proximity, so proximity-in fires once on the first call. Finger
    /// touches call this too (see the module doc comment) -- `pressure`
    /// should still be a real nonzero value while `in_contact` for those,
    /// it's forced to 0 automatically on release regardless of what's
    /// passed in.
    pub fn emit(
        &self,
        x: i32,
        y: i32,
        pressure: i32,
        tilt_x: i32,
        tilt_y: i32,
        in_contact: bool,
    ) -> io::Result<()> {
        let t = EventTime::new(0, 0); // kernel fills in the real timestamp
        let mut events = Vec::with_capacity(8);

        if !self.tool_in_proximity.replace(true) {
            events.push(InputEvent { time: t, kind: EventKind::Key, code: Key::ButtonToolPen as u16, value: 1 });
        }
        if self.in_contact.replace(in_contact) != in_contact {
            events.push(InputEvent { time: t, kind: EventKind::Key, code: Key::ButtonTouch as u16, value: in_contact as i32 });
        }
        events.push(InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::X as u16, value: x });
        events.push(InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::Y as u16, value: y });
        // ABS_PRESSURE must always be sent, including for finger touches
        // (confirmed live: skipping it entirely -- pressure/tilt aren't
        // semantically meaningful for a finger -- left the cursor
        // completely unresponsive to finger input; libinput's tablet-tool
        // motion handling on this device appears to require nonzero
        // pressure to move the cursor at all), and must be forced to 0 when
        // not in contact (confirmed live: a stale nonzero pressure value on
        // release left the pointer stuck "down" until the next real press).
        let effective_pressure = if in_contact { pressure } else { 0 };
        events.extend([
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::Pressure as u16, value: effective_pressure },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::TiltX as u16, value: tilt_x },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::TiltY as u16, value: tilt_y },
        ]);
        events.push(InputEvent { time: t, kind: EventKind::Synchronize, code: SynchronizeKind::Report as u16, value: 0 });
        let raw: Vec<input_linux::sys::input_event> = events.into_iter().map(Into::into).collect();
        self.handle.write(&raw)?;
        Ok(())
    }

    /// Toggles the S Pen side button (`BTN_STYLUS`), independent of any
    /// position update -- edge-triggered like everything else here.
    pub fn set_button(&self, pressed: bool) -> io::Result<()> {
        if self.button_pressed.replace(pressed) == pressed {
            return Ok(());
        }
        let t = EventTime::new(0, 0);
        let events = [
            InputEvent { time: t, kind: EventKind::Key, code: Key::ButtonStylus as u16, value: pressed as i32 },
            InputEvent { time: t, kind: EventKind::Synchronize, code: SynchronizeKind::Report as u16, value: 0 },
        ];
        let raw: Vec<input_linux::sys::input_event> = events.into_iter().map(Into::into).collect();
        self.handle.write(&raw)?;
        Ok(())
    }
}

impl Drop for UinputTablet {
    fn drop(&mut self) {
        let _ = self.handle.dev_destroy();
    }
}
