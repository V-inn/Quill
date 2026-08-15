//! Virtual uinput **touchpad**: a multitouch device libinput classifies as a
//! touchpad, so libinput does the gesture recognition rather than us.
//!
//! # Why a second device instead of extending the tablet
//!
//! Milestone 6b tried teaching the tablet device about fingers by adding
//! `BTN_TOOL_FINGER` to it, and the cursor stopped moving entirely -- that bit
//! is how touchpads identify themselves, and libinput evidently reclassified
//! the device into touchpad (relative) semantics for finger-tagged events. The
//! conclusion recorded there was that fingers need "a genuinely different
//! device model", which is this file: the tablet stays a pure tablet, and
//! multi-finger input goes to a device that *is* a touchpad and is treated
//! like one.
//!
//! What that buys, none of which we implement: two-finger scroll (delivered as
//! ordinary wheel events, so it works in every app including XWayland ones),
//! pinch and swipe gestures, tap-to-click, two-finger tap as right click,
//! kinetic scrolling, palm/thumb handling -- and all of it configurable in
//! System Settings -> Touchpad, on GNOME as well as KDE, since both drive
//! libinput.
//!
//! # The capability set is copied from real hardware, deliberately
//!
//! This machine's own touchpad reports `PROP=5`
//! (`INPUT_PROP_POINTER | INPUT_PROP_BUTTONPAD`) with `BTN_TOOL_FINGER`,
//! `BTN_TOOL_DOUBLETAP`, `BTN_TOOL_TRIPLETAP`, `BTN_TOOL_QUADTAP`,
//! `BTN_TOOL_QUINTTAP`, `BTN_TOUCH` and `BTN_LEFT`. We declare the same set
//! minus `INPUT_PROP_BUTTONPAD` (we have no physical button under the surface
//! to click) and minus quinttap (four contacts is plenty for gestures).
//!
//! Contacts are reported in the kernel's multitouch protocol **type B**: a
//! slot is selected, its tracking id is set (and set to `-1` to lift it), and
//! its position follows. Slot state persists between frames, so only what
//! actually changed goes on the wire.

use input_linux::{
    AbsoluteAxis, AbsoluteInfo, AbsoluteInfoSetup, EventKind, EventTime, InputEvent, InputId,
    InputProperty, Key, SynchronizeKind, UInputHandle,
};
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io;

/// Four contacts is enough for every gesture libinput recognizes (pinch and
/// swipe top out at four fingers), and each slot costs state on both sides.
pub const MAX_SLOTS: usize = 4;

/// Physical geometry of the tablet's touch surface, from the capability
/// handshake -- never hardcoded, same rule as [`crate::uinput_tablet`].
///
/// `res_x`/`res_y` are units per millimetre. libinput derives the device's
/// physical size from the axis range divided by this, and its thresholds
/// (scroll distance, tap slop, pinch detection) are all specified in
/// millimetres -- so a wrong resolution here doesn't fail loudly, it just
/// makes every gesture threshold wrong. `uinput_tablet.rs` records what a
/// resolution of `0` did to the tablet: nothing moved at all.
pub struct TouchpadGeometry {
    pub width: i32,
    pub height: i32,
    pub res_x: i32,
    pub res_y: i32,
}

pub struct UinputTouchpad {
    handle: UInputHandle<File>,
    /// Which slot `ABS_MT_SLOT` currently points at, so a run of events for one
    /// contact doesn't re-select it every frame (the kernel keeps this state).
    current_slot: Cell<i32>,
    /// Monotonic; a contact's id must be unique among *live* contacts, and
    /// reusing an id after a lift is what tells userspace it is a new touch.
    next_tracking_id: Cell<i32>,
    active: [Cell<bool>; MAX_SLOTS],
    /// Last contact count published via the `BTN_TOOL_*` bits, so those are
    /// edge-triggered like every other key event in this project.
    published_count: Cell<usize>,
}

impl UinputTouchpad {
    pub fn create(geometry: &TouchpadGeometry) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")?;
        let handle = UInputHandle::new(file);

        // Pointer, not Direct: Direct is what marks a touchscreen (input lands
        // where you touch), and libinput does not do gestures on those at all.
        handle.set_propbit(InputProperty::Pointer)?;

        handle.set_evbit(EventKind::Key)?;
        for key in [
            Key::ButtonLeft,
            Key::ButtonRight,
            Key::ButtonTouch,
            Key::ButtonToolFinger,
            Key::ButtonToolDoubleTap,
            Key::ButtonToolTripleTap,
            Key::ButtonToolQuadtap,
        ] {
            handle.set_keybit(key)?;
        }

        handle.set_evbit(EventKind::Absolute)?;
        for axis in [
            AbsoluteAxis::X,
            AbsoluteAxis::Y,
            AbsoluteAxis::MultitouchSlot,
            AbsoluteAxis::MultitouchPositionX,
            AbsoluteAxis::MultitouchPositionY,
            AbsoluteAxis::MultitouchTrackingId,
        ] {
            handle.set_absbit(axis)?;
        }

        let position = |axis, maximum, resolution| AbsoluteInfoSetup {
            axis,
            info: AbsoluteInfo {
                value: 0,
                minimum: 0,
                maximum,
                fuzz: 0,
                flat: 0,
                resolution,
            },
        };
        let abs = [
            // ABS_X/ABS_Y mirror slot 0 throughout. libinput reads the
            // multitouch axes on a device that has them, but the single-touch
            // pair is what a device is *classified* on, and real touchpads
            // report both.
            position(AbsoluteAxis::X, geometry.width, geometry.res_x),
            position(AbsoluteAxis::Y, geometry.height, geometry.res_y),
            position(
                AbsoluteAxis::MultitouchPositionX,
                geometry.width,
                geometry.res_x,
            ),
            position(
                AbsoluteAxis::MultitouchPositionY,
                geometry.height,
                geometry.res_y,
            ),
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::MultitouchSlot,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: MAX_SLOTS as i32 - 1,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::MultitouchTrackingId,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: 0xFFFF,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
        ];

        // Same fixed-identity reasoning as the tablet (MILESTONES.md Milestone
        // 6b): the desktop keys its per-device settings off this, so it has to
        // stay stable across daemon restarts. Distinct product id, since this
        // is a distinct device with its own settings page.
        let id = InputId {
            bustype: input_linux::sys::BUS_VIRTUAL as u16,
            vendor: 0x1209, // pid.codes test/prototype vendor ID
            product: 0x0002,
            version: 1,
        };
        handle.create(&id, b"Quill Virtual Touchpad", 0, &abs)?;

        Ok(Self {
            handle,
            current_slot: Cell::new(-1),
            next_tracking_id: Cell::new(1),
            active: std::array::from_fn(|_| Cell::new(false)),
            published_count: Cell::new(0),
        })
    }

    fn contact_count(&self) -> usize {
        self.active.iter().filter(|a| a.get()).count()
    }

    /// Emits `ABS_MT_SLOT` only when the kernel isn't already pointing at this
    /// slot -- redundant selects are harmless but this is the shape real
    /// drivers have, and it keeps the event stream readable in
    /// `libinput debug-events`.
    fn select_slot(&self, slot: i32, events: &mut Vec<InputEvent>, t: EventTime) {
        if self.current_slot.replace(slot) != slot {
            events.push(InputEvent {
                time: t,
                kind: EventKind::Absolute,
                code: AbsoluteAxis::MultitouchSlot as u16,
                value: slot,
            });
        }
    }

    /// `BTN_TOUCH` plus exactly one `BTN_TOOL_*` bit for the live contact
    /// count, both edge-triggered. libinput can count contacts from the slots
    /// alone, but the tool bits are what it uses on devices that report them,
    /// and every real touchpad reports them.
    fn publish_count(&self, events: &mut Vec<InputEvent>, t: EventTime) {
        let count = self.contact_count();
        let previous = self.published_count.replace(count);
        if count == previous {
            return;
        }
        let tool_for = |n: usize| match n {
            1 => Some(Key::ButtonToolFinger),
            2 => Some(Key::ButtonToolDoubleTap),
            3 => Some(Key::ButtonToolTripleTap),
            0 => None,
            _ => Some(Key::ButtonToolQuadtap),
        };
        let (old_tool, new_tool) = (tool_for(previous), tool_for(count));
        if old_tool != new_tool {
            if let Some(tool) = old_tool {
                events.push(InputEvent {
                    time: t,
                    kind: EventKind::Key,
                    code: tool as u16,
                    value: 0,
                });
            }
            if let Some(tool) = new_tool {
                events.push(InputEvent {
                    time: t,
                    kind: EventKind::Key,
                    code: tool as u16,
                    value: 1,
                });
            }
        }
        if (previous == 0) != (count == 0) {
            events.push(InputEvent {
                time: t,
                kind: EventKind::Key,
                code: Key::ButtonTouch as u16,
                value: (count > 0) as i32,
            });
        }
    }

    fn position_events(
        &self,
        slot: i32,
        x: i32,
        y: i32,
        events: &mut Vec<InputEvent>,
        t: EventTime,
    ) {
        events.push(InputEvent {
            time: t,
            kind: EventKind::Absolute,
            code: AbsoluteAxis::MultitouchPositionX as u16,
            value: x,
        });
        events.push(InputEvent {
            time: t,
            kind: EventKind::Absolute,
            code: AbsoluteAxis::MultitouchPositionY as u16,
            value: y,
        });
        // Single-touch emulation tracks the lowest live slot, the same
        // convention the kernel's own input-mt helpers use.
        if self.active.iter().take(slot as usize).all(|a| !a.get()) {
            events.push(InputEvent {
                time: t,
                kind: EventKind::Absolute,
                code: AbsoluteAxis::X as u16,
                value: x,
            });
            events.push(InputEvent {
                time: t,
                kind: EventKind::Absolute,
                code: AbsoluteAxis::Y as u16,
                value: y,
            });
        }
    }

    pub fn touch_down(&self, slot: usize, x: i32, y: i32) -> io::Result<()> {
        let Some(active) = self.active.get(slot) else {
            return Ok(());
        };
        let t = EventTime::new(0, 0);
        let mut events = Vec::with_capacity(10);
        self.select_slot(slot as i32, &mut events, t);
        let id = self.next_tracking_id.get();
        self.next_tracking_id
            .set(if id >= 0xFFFF { 1 } else { id + 1 });
        events.push(InputEvent {
            time: t,
            kind: EventKind::Absolute,
            code: AbsoluteAxis::MultitouchTrackingId as u16,
            value: id,
        });
        active.set(true);
        self.position_events(slot as i32, x, y, &mut events, t);
        self.publish_count(&mut events, t);
        self.sync(events)
    }

    pub fn touch_move(&self, slot: usize, x: i32, y: i32) -> io::Result<()> {
        let Some(active) = self.active.get(slot) else {
            return Ok(());
        };
        if !active.get() {
            // A move for a contact we never saw go down: treat it as the down,
            // rather than emitting a position for an unslotted contact, which
            // libinput would ignore anyway.
            return self.touch_down(slot, x, y);
        }
        let t = EventTime::new(0, 0);
        let mut events = Vec::with_capacity(6);
        self.select_slot(slot as i32, &mut events, t);
        self.position_events(slot as i32, x, y, &mut events, t);
        self.sync(events)
    }

    pub fn touch_up(&self, slot: usize) -> io::Result<()> {
        let Some(active) = self.active.get(slot) else {
            return Ok(());
        };
        if !active.replace(false) {
            return Ok(());
        }
        let t = EventTime::new(0, 0);
        let mut events = Vec::with_capacity(4);
        self.select_slot(slot as i32, &mut events, t);
        events.push(InputEvent {
            time: t,
            kind: EventKind::Absolute,
            code: AbsoluteAxis::MultitouchTrackingId as u16,
            value: -1,
        });
        self.publish_count(&mut events, t);
        self.sync(events)
    }

    /// Lifts every live contact. Used when a connection drops mid-gesture --
    /// otherwise the contacts stay down forever from libinput's point of view
    /// and the next session starts with phantom fingers on the pad.
    pub fn release_all(&self) -> io::Result<()> {
        for slot in 0..MAX_SLOTS {
            if self.active[slot].get() {
                self.touch_up(slot)?;
            }
        }
        Ok(())
    }

    fn sync(&self, mut events: Vec<InputEvent>) -> io::Result<()> {
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

impl Drop for UinputTouchpad {
    fn drop(&mut self) {
        let _ = self.handle.dev_destroy();
    }
}
