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
    /// Whether the side button is currently being held as an eraser.
    eraser_active: Cell<bool>,
    /// Which tool the device last declared in proximity.
    tool_is_rubber: Cell<bool>,
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

/// Cheap, non-destructive check for whether `/dev/uinput` is actually
/// usable before committing to the uinput input path -- on a machine with
/// no root access, ever (e.g. a school computer), the device node exists
/// but is root-owned with no ACL/uaccess grant, and opening it fails with
/// permission denied. `main.rs` uses this to decide between the real
/// tablet-fidelity input path and the reduced-fidelity portal
/// `RemoteDesktop` fallback (`remote_desktop_input.rs`) before doing any
/// portal negotiation, since the two paths need different (and mutually
/// exclusive) portal sessions.
pub fn uinput_accessible() -> bool {
    // Test knob for the RemoteDesktop fallback path on a machine that
    // actually does have uinput access -- there's no way to safely fake a
    // real permission-denied /dev/uinput without root, which defeats the
    // purpose of testing "what a no-root machine sees".
    if std::env::var("QUILL_FORCE_NO_UINPUT").is_ok() {
        return false;
    }
    OpenOptions::new().read(true).write(true).open("/dev/uinput").is_ok()
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
        // Declared whatever the side button is currently mapped to: keybits are
        // fixed when the device is created, but the mapping is a live setting,
        // so the device has to be able to speak all of them from the start.
        handle.set_keybit(Key::ButtonStylus2)?;
        handle.set_keybit(Key::ButtonToolRubber)?;

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
            eraser_active: Cell::new(false),
            tool_is_rubber: Cell::new(false),
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

        // Which tool is in proximity. The eraser is not a button in evdev's
        // model -- it is a *different tool*, so holding the side button in
        // eraser mode swaps BTN_TOOL_PEN for BTN_TOOL_RUBBER rather than
        // reporting a press.
        let want_rubber = self.eraser_active.get();
        if !self.tool_in_proximity.replace(true) {
            events.push(InputEvent { time: t, kind: EventKind::Key, code: tool_key(want_rubber), value: 1 });
            self.tool_is_rubber.set(want_rubber);
        } else if self.tool_is_rubber.get() != want_rubber {
            events.push(InputEvent { time: t, kind: EventKind::Key, code: tool_key(self.tool_is_rubber.get()), value: 0 });
            events.push(InputEvent { time: t, kind: EventKind::Key, code: tool_key(want_rubber), value: 1 });
            self.tool_is_rubber.set(want_rubber);
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

    /// Acts on the S Pen side button, independent of any position update --
    /// edge-triggered like everything else here.
    ///
    /// `action` is chosen on the tablet and carried per event, so changing the
    /// mapping takes effect on the next press with no reconnect. See
    /// `input_receiver`'s protocol note.
    pub fn set_button(&self, pressed: bool, action: ButtonAction) -> io::Result<()> {
        if self.button_pressed.replace(pressed) == pressed {
            return Ok(());
        }
        let t = EventTime::new(0, 0);
        let mut events = Vec::with_capacity(4);
        match action {
            ButtonAction::RightClick => events.push(InputEvent {
                time: t, kind: EventKind::Key, code: Key::ButtonStylus as u16, value: pressed as i32,
            }),
            ButtonAction::MiddleClick => events.push(InputEvent {
                time: t, kind: EventKind::Key, code: Key::ButtonStylus2 as u16, value: pressed as i32,
            }),
            ButtonAction::Eraser => {
                self.eraser_active.set(pressed);
                // Swap the tool now rather than waiting for the next position
                // update: the pen is often held still while the button is
                // pressed, and a tool that changes only once you move would
                // feel broken.
                if self.tool_in_proximity.get() && self.tool_is_rubber.get() != pressed {
                    events.push(InputEvent {
                        time: t, kind: EventKind::Key, code: tool_key(self.tool_is_rubber.get()), value: 0,
                    });
                    events.push(InputEvent {
                        time: t, kind: EventKind::Key, code: tool_key(pressed), value: 1,
                    });
                    self.tool_is_rubber.set(pressed);
                }
            }
        }
        if events.is_empty() {
            return Ok(());
        }
        events.push(InputEvent { time: t, kind: EventKind::Synchronize, code: SynchronizeKind::Report as u16, value: 0 });
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

/// What the S Pen's side button does while held.
///
/// Chosen on the tablet and sent with each button event rather than negotiated
/// at connect time, so changing it takes effect on the next press. The device
/// declares the key bits for all of these when it is created, because keybits
/// are fixed at creation and the mapping is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonAction {
    /// `BTN_STYLUS` -- what a stylus's first side button conventionally is,
    /// and what every version before the mapping existed always sent.
    #[default]
    RightClick,
    /// `BTN_STYLUS2`, the second side button.
    MiddleClick,
    /// Not a button at all in evdev's model: swaps `BTN_TOOL_PEN` for
    /// `BTN_TOOL_RUBBER` while held, which is how a real tablet reports the
    /// other end of the pen.
    Eraser,
}

impl ButtonAction {
    /// Decodes bits 2-3 of a button event's `buttons` byte. Anything
    /// unrecognised falls back to the historical behaviour rather than doing
    /// nothing, so a newer client talking to this daemon degrades to a right
    /// click instead of a dead button.
    pub fn from_event_buttons(buttons: u8) -> Self {
        match (buttons >> 2) & 0b11 {
            1 => ButtonAction::MiddleClick,
            2 => ButtonAction::Eraser,
            _ => ButtonAction::RightClick,
        }
    }
}

fn tool_key(rubber: bool) -> u16 {
    if rubber { Key::ButtonToolRubber as u16 } else { Key::ButtonToolPen as u16 }
}

#[cfg(test)]
mod button_action_tests {
    use super::ButtonAction;

    /// Bits 0 and 1 of this byte mean other things (stylus state, finger tool),
    /// so the mapping has to ignore them.
    #[test]
    fn the_other_bits_are_ignored() {
        assert_eq!(ButtonAction::from_event_buttons(0b0000_0011), ButtonAction::RightClick);
        assert_eq!(ButtonAction::from_event_buttons(0b0000_0111), ButtonAction::MiddleClick);
        assert_eq!(ButtonAction::from_event_buttons(0b0000_1011), ButtonAction::Eraser);
    }

    /// A client that predates the mapping sends zero there, and must keep
    /// getting exactly what it always got.
    #[test]
    fn an_older_client_still_right_clicks() {
        assert_eq!(ButtonAction::from_event_buttons(0b0000_0001), ButtonAction::RightClick);
        assert_eq!(ButtonAction::from_event_buttons(0), ButtonAction::RightClick);
    }

    #[test]
    fn an_unknown_action_degrades_rather_than_dying() {
        assert_eq!(ButtonAction::from_event_buttons(0b0000_1100), ButtonAction::RightClick);
    }
}
