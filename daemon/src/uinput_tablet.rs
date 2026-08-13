//! Virtual uinput tablet device: a pen/stylus input device libinput (and
//! therefore GIMP/Krita/every other app) recognizes as a real graphics
//! tablet -- pressure, tilt, hover-proximity, and the S Pen side button.
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
        })
    }

    /// One full pen-position update: move to (x, y), set pressure/tilt, set
    /// contact (BTN_TOUCH + BTN_TOOL_PEN), then SYN_REPORT so the kernel
    /// delivers it as one atomic input frame. Key/button events are only
    /// emitted on an actual state transition (see the `tool_in_proximity`/
    /// `in_contact` doc comment above) -- calling this at all implies the
    /// tool is in proximity, so proximity-in fires once on the first call.
    pub fn emit(&self, x: i32, y: i32, pressure: i32, tilt_x: i32, tilt_y: i32, in_contact: bool) -> io::Result<()> {
        let t = EventTime::new(0, 0); // kernel fills in the real timestamp
        let mut events = Vec::with_capacity(8);

        if !self.tool_in_proximity.replace(true) {
            events.push(InputEvent { time: t, kind: EventKind::Key, code: Key::ButtonToolPen as u16, value: 1 });
        }
        if self.in_contact.replace(in_contact) != in_contact {
            events.push(InputEvent { time: t, kind: EventKind::Key, code: Key::ButtonTouch as u16, value: in_contact as i32 });
        }
        events.extend([
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::X as u16, value: x },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::Y as u16, value: y },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::Pressure as u16, value: pressure },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::TiltX as u16, value: tilt_x },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::TiltY as u16, value: tilt_y },
            InputEvent { time: t, kind: EventKind::Synchronize, code: SynchronizeKind::Report as u16, value: 0 },
        ]);
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
