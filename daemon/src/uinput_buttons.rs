//! A small virtual pointer device for the things neither the tablet nor the
//! touchpad can express: a right click that isn't the S Pen's barrel button,
//! ctrl+wheel zoom, and -- the reason it reports absolute coordinates --
//! putting the pointer where the fingers actually are.
//!
//! # Why absolute coordinates
//!
//! Confirmed live: gestures landed on whichever screen the mouse pointer had
//! last been left on, not the one being touched. Scroll, wheel and button
//! events are delivered to whatever the *pointer* is over, and neither of the
//! other two devices puts it over the tablet's output. The touchpad is
//! relative and only ever sees two fingers or more, so it never emits pointer
//! motion at all; the tablet is a tablet, and its tool cursor is a separate
//! thing from the pointer as far as delivery is concerned. So the pointer
//! stayed wherever the mouse left it and the gesture went there.
//!
//! This device carries `ABS_X`/`ABS_Y` across the whole desktop's logical
//! geometry (see `orientation::layout`), so a warp before a gesture or a click
//! moves the real pointer onto the virtual output first. Absolute pointer
//! devices are mapped across the entire desktop by default -- the same
//! behaviour Milestone 6b had to work around for the *tablet* with a per-device
//! mapping in System Settings -- which here is exactly what is wanted, since
//! the daemon does the mapping itself and needs the full space to aim in.
//!
//! Kept separate from both other devices on purpose. The tablet is classified
//! as a graphics tablet and the touchpad as a touchpad; adding relative wheel
//! axes or a plain keyboard key to either invites exactly the reclassification
//! that broke finger input in Milestone 6b. This device is what it looks like:
//! a mouse with a control key.
//!
//! It is created only when something needs it -- long-press right click, or
//! the client asking for ctrl+scroll zoom -- so a default session still
//! presents just the tablet and the touchpad.

use input_linux::{
    AbsoluteAxis, AbsoluteInfo, AbsoluteInfoSetup, EventKind, EventTime, InputEvent, InputId,
    InputProperty, Key, RelativeAxis, SynchronizeKind, UInputHandle,
};
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io;

/// High-resolution wheel units per detent, fixed by the kernel's input API.
const HI_RES_PER_CLICK: i32 = 120;

pub struct UinputButtons {
    handle: UInputHandle<File>,
    right_pressed: Cell<bool>,
    ctrl_held: Cell<bool>,
}

impl UinputButtons {
    /// `desktop` is the logical bounding box of every enabled output, which the
    /// absolute axes span; coordinates passed to [`Self::warp`] are in that
    /// same space.
    pub fn create(desktop: crate::orientation::Rect) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")?;
        let handle = UInputHandle::new(file);

        // Pointer and not Direct, buttons but no BTN_TOUCH and no multitouch
        // axes: that combination is what gets this tagged ID_INPUT_MOUSE and
        // treated as a pointer reporting absolute positions, rather than as a
        // touchscreen or a tablet.
        handle.set_propbit(InputProperty::Pointer)?;

        handle.set_evbit(EventKind::Key)?;
        for key in [
            Key::ButtonLeft,
            Key::ButtonRight,
            Key::ButtonMiddle,
            Key::LeftCtrl,
        ] {
            handle.set_keybit(key)?;
        }

        handle.set_evbit(EventKind::Relative)?;
        for axis in [
            RelativeAxis::Wheel,
            RelativeAxis::HorizontalWheel,
            RelativeAxis::WheelHiRes,
            RelativeAxis::HorizontalWheelHiRes,
        ] {
            handle.set_relbit(axis)?;
        }

        handle.set_evbit(EventKind::Absolute)?;
        handle.set_absbit(AbsoluteAxis::X)?;
        handle.set_absbit(AbsoluteAxis::Y)?;
        let screen_axis = |axis, maximum| AbsoluteInfoSetup {
            axis,
            info: AbsoluteInfo {
                value: 0,
                minimum: 0,
                maximum,
                fuzz: 0,
                flat: 0,
                // Deliberately 0 here, unlike the tablet and the touchpad:
                // this axis is a screen coordinate, not a physical surface, so
                // there is no units/mm that would mean anything.
                resolution: 0,
            },
        };
        let abs = [
            screen_axis(AbsoluteAxis::X, desktop.w.round().max(1.0) as i32),
            screen_axis(AbsoluteAxis::Y, desktop.h.round().max(1.0) as i32),
        ];

        let id = InputId {
            bustype: input_linux::sys::BUS_VIRTUAL as u16,
            vendor: 0x1209, // pid.codes test/prototype vendor ID
            product: 0x0003,
            version: 1,
        };
        handle.create(&id, b"Quill Virtual Buttons", 0, &abs)?;

        Ok(Self {
            handle,
            right_pressed: Cell::new(false),
            ctrl_held: Cell::new(false),
        })
    }

    /// Puts the pointer at a logical desktop coordinate, so whatever follows --
    /// a wheel click, a right button -- lands on what the fingers are over
    /// rather than wherever the mouse was last left.
    pub fn warp(&self, x: i32, y: i32) -> io::Result<()> {
        let t = EventTime::new(0, 0);
        self.emit(vec![
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::X as u16, value: x },
            InputEvent { time: t, kind: EventKind::Absolute, code: AbsoluteAxis::Y as u16, value: y },
        ])
    }

    /// Edge-triggered, like every other key in this project -- a long press
    /// that is held is one press event, not one per input frame.
    pub fn set_right_button(&self, pressed: bool) -> io::Result<()> {
        if self.right_pressed.replace(pressed) == pressed {
            return Ok(());
        }
        self.emit(vec![InputEvent {
            time: EventTime::new(0, 0),
            kind: EventKind::Key,
            code: Key::ButtonRight as u16,
            value: pressed as i32,
        }])
    }

    pub fn set_ctrl(&self, held: bool) -> io::Result<()> {
        if self.ctrl_held.replace(held) == held {
            return Ok(());
        }
        self.emit(vec![InputEvent {
            time: EventTime::new(0, 0),
            kind: EventKind::Key,
            code: Key::LeftCtrl as u16,
            value: held as i32,
        }])
    }

    /// Positive scrolls up / zooms in. Both the classic detent axis and the
    /// high-resolution one are sent, which is what current kernel drivers do:
    /// consumers that understand hi-res use it and ignore the other.
    pub fn wheel(&self, clicks: i32) -> io::Result<()> {
        if clicks == 0 {
            return Ok(());
        }
        let t = EventTime::new(0, 0);
        self.emit(vec![
            InputEvent {
                time: t,
                kind: EventKind::Relative,
                code: RelativeAxis::Wheel as u16,
                value: clicks,
            },
            InputEvent {
                time: t,
                kind: EventKind::Relative,
                code: RelativeAxis::WheelHiRes as u16,
                value: clicks * HI_RES_PER_CLICK,
            },
        ])
    }

    /// Releases anything this device is holding. A connection that drops with
    /// ctrl held would leave the whole desktop in a modifier-stuck state, which
    /// is far worse than the gesture being cut short.
    pub fn release_all(&self) -> io::Result<()> {
        self.set_right_button(false)?;
        self.set_ctrl(false)
    }

    fn emit(&self, mut events: Vec<InputEvent>) -> io::Result<()> {
        events.push(InputEvent {
            time: EventTime::new(0, 0),
            kind: EventKind::Synchronize,
            code: SynchronizeKind::Report as u16,
            value: 0,
        });
        let raw: Vec<input_linux::sys::input_event> = events.into_iter().map(Into::into).collect();
        self.handle.write(&raw)?;
        Ok(())
    }
}

impl Drop for UinputButtons {
    fn drop(&mut self) {
        let _ = self.release_all();
        let _ = self.handle.dev_destroy();
    }
}
