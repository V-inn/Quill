//! Two-finger gesture recognition, in Rust, with no I/O of its own.
//!
//! # Why this exists when libinput already does it
//!
//! Two reasons, both of which need the *same* recognizer:
//!
//! - **Ctrl+scroll zoom.** libinput's pinch arrives as a Wayland gesture, so
//!   only gesture-aware toolkits act on it -- anything on XWayland ignores it
//!   entirely. When the client asks for ctrl+scroll zoom instead
//!   (`CONFIG_CTRL_SCROLL_ZOOM`), something has to decide "this is a pinch,
//!   not a pan" *before* the contacts reach the touchpad device, so the pinch
//!   can be withheld from it and re-emitted as ctrl+wheel.
//! - **The no-uinput path.** On a machine with no `/dev/uinput` access -- the
//!   school-computer case, and the GNOME case in Milestone 10 -- there is no
//!   device to hand raw contacts to. The `RemoteDesktop` portal has
//!   `NotifyPointerAxis`/`NotifyPointerButton` and no gesture API at all, so
//!   gestures there can only come from a recognizer of ours. That wiring is
//!   deferred; this module is the part it will need.
//!
//! # What it recognizes
//!
//! Exactly two contacts, and only the two gestures that have a meaning on the
//! other side: scroll (both fingers translating together) and pinch (the
//! distance between them changing). Everything else -- taps, swipes, three-
//! and four-finger gestures, kinetic scrolling -- is left to libinput, which
//! does it better and is configurable in the desktop's own settings.
//!
//! A gesture stays [`Classification::Undecided`] until one of the two
//! thresholds is crossed, and once decided it never changes for the life of
//! that two-finger contact. Deciding once matters: a pinch that briefly looks
//! like a pan (or vice versa) would otherwise oscillate between two output
//! devices mid-gesture.

use crate::uinput_touchpad::MAX_SLOTS;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Classification {
    /// Fewer than two contacts, or neither threshold crossed yet.
    Undecided,
    Scroll,
    Pinch,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gesture {
    /// Movement of the two-finger centroid since the previous frame, in device
    /// units. Positive `dy` is downward, matching the touch surface; whether
    /// that scrolls the content up or down is the consumer's business.
    Scroll { dx: f32, dy: f32 },
    /// Ratio of the current finger separation to the previous frame's.
    /// Above 1.0 is a spread (zoom in), below is a pinch (zoom out).
    Pinch { ratio: f32 },
}

/// Thresholds, in device units, before an undecided gesture commits. Both are
/// computed from the surface resolution by [`Recognizer::new`] so they mean
/// the same physical distance on any tablet -- the same reason the touchpad
/// device reports a real units/mm resolution.
const PAN_THRESHOLD_MM: f32 = 2.0;
const PINCH_THRESHOLD_MM: f32 = 3.0;

#[derive(Clone, Copy)]
struct Contact {
    x: f32,
    y: f32,
}

pub struct Recognizer {
    contacts: [Option<Contact>; MAX_SLOTS],
    classification: Classification,
    /// Centroid and separation when the second finger landed, for threshold
    /// comparison, and as of the previous frame, for the emitted deltas.
    start_centroid: (f32, f32),
    start_distance: f32,
    last_centroid: (f32, f32),
    last_distance: f32,
    pan_threshold: f32,
    pinch_threshold: f32,
}

impl Recognizer {
    /// `res_x`/`res_y` are the touch surface's units per millimetre, as
    /// reported in the capability handshake.
    pub fn new(res_x: i32, res_y: i32) -> Self {
        // Average the two axes: a threshold per axis would make a diagonal
        // gesture commit at a different distance than a straight one.
        let units_per_mm = ((res_x.max(1) + res_y.max(1)) as f32) / 2.0;
        Self {
            contacts: [None; MAX_SLOTS],
            classification: Classification::Undecided,
            start_centroid: (0.0, 0.0),
            start_distance: 0.0,
            last_centroid: (0.0, 0.0),
            last_distance: 0.0,
            pan_threshold: PAN_THRESHOLD_MM * units_per_mm,
            pinch_threshold: PINCH_THRESHOLD_MM * units_per_mm,
        }
    }

    pub fn classification(&self) -> Classification {
        self.classification
    }

    pub fn contact_count(&self) -> usize {
        self.contacts.iter().filter(|c| c.is_some()).count()
    }

    /// Midpoint of the two live contacts, in device units. Used to aim the
    /// pointer at where the gesture is actually happening -- see
    /// `input_receiver::warp_pointer`.
    pub fn centroid(&self) -> Option<(i32, i32)> {
        if self.contact_count() != 2 {
            return None;
        }
        let (c, _) = self.geometry();
        Some((c.0.round() as i32, c.1.round() as i32))
    }

    pub fn down(&mut self, slot: usize, x: i32, y: i32) {
        self.set(
            slot,
            Some(Contact {
                x: x as f32,
                y: y as f32,
            }),
        );
    }

    pub fn up(&mut self, slot: usize) {
        self.set(slot, None);
    }

    fn set(&mut self, slot: usize, contact: Option<Contact>) {
        if slot >= MAX_SLOTS {
            return;
        }
        self.contacts[slot] = contact;
        if self.contact_count() == 2 {
            // (Re)anchor whenever the pair changes, including when a third
            // finger lifts back down to two -- the old anchor describes a
            // different set of fingers.
            let (centroid, distance) = self.geometry();
            self.start_centroid = centroid;
            self.start_distance = distance;
            self.last_centroid = centroid;
            self.last_distance = distance;
        }
        if self.contact_count() < 2 {
            self.classification = Classification::Undecided;
        }
    }

    /// Feeds a contact's new position. Returns a gesture once this contact
    /// pair has committed to one, and `None` while undecided or while fewer
    /// than two fingers are down.
    pub fn motion(&mut self, slot: usize, x: i32, y: i32) -> Option<Gesture> {
        if slot >= MAX_SLOTS || self.contacts[slot].is_none() {
            return None;
        }
        self.contacts[slot] = Some(Contact {
            x: x as f32,
            y: y as f32,
        });
        if self.contact_count() != 2 {
            return None;
        }
        let (centroid, distance) = self.geometry();

        if self.classification == Classification::Undecided {
            let moved = ((centroid.0 - self.start_centroid.0).powi(2)
                + (centroid.1 - self.start_centroid.1).powi(2))
            .sqrt();
            let spread = (distance - self.start_distance).abs();
            // Pinch is tested first: fingers moving apart also move the
            // centroid a little, so a pan threshold checked first would claim
            // most slow pinches.
            self.classification = if spread > self.pinch_threshold {
                Classification::Pinch
            } else if moved > self.pan_threshold {
                Classification::Scroll
            } else {
                Classification::Undecided
            };
        }

        let gesture = match self.classification {
            Classification::Undecided => None,
            Classification::Scroll => Some(Gesture::Scroll {
                dx: centroid.0 - self.last_centroid.0,
                dy: centroid.1 - self.last_centroid.1,
            }),
            Classification::Pinch => {
                // Guard the degenerate case rather than emitting inf: two
                // contacts reported at the same coordinates is physically
                // impossible but arrives from a corrupted stream.
                let ratio = if self.last_distance > 1.0 {
                    distance / self.last_distance
                } else {
                    1.0
                };
                Some(Gesture::Pinch { ratio })
            }
        };
        self.last_centroid = centroid;
        self.last_distance = distance;
        gesture
    }

    fn geometry(&self) -> ((f32, f32), f32) {
        let live: Vec<Contact> = self.contacts.iter().flatten().copied().collect();
        if live.len() < 2 {
            return ((0.0, 0.0), 0.0);
        }
        let (a, b) = (live[0], live[1]);
        let centroid = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        (centroid, distance)
    }
}

/// Turns a stream of pinch ratios into discrete wheel clicks, for the
/// ctrl+scroll zoom mode.
///
/// Zoom is multiplicative, so the accumulator works in log space: every
/// [`ZOOM_STEP`] doubling-fraction of separation is one wheel click. Doing it
/// on the raw ratio instead would make a spread near the start of a gesture
/// worth far more than the same spread later.
pub struct ZoomAccumulator {
    accumulated: f32,
}

/// One wheel click per ~12% change in finger separation. Chosen so a
/// comfortable pinch across the tablet is a handful of clicks rather than
/// dozens -- most applications zoom by a fixed factor per click, so this is
/// the knob that decides how fast ctrl+scroll zoom feels.
const ZOOM_STEP: f32 = 0.12;

impl Default for ZoomAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoomAccumulator {
    pub fn new() -> Self {
        Self { accumulated: 0.0 }
    }

    /// Returns how many wheel clicks to emit (positive zooms in), keeping the
    /// remainder for the next frame so slow pinches aren't quantized away.
    pub fn feed(&mut self, ratio: f32) -> i32 {
        if !(ratio.is_finite() && ratio > 0.0) {
            return 0;
        }
        self.accumulated += ratio.ln();
        let clicks = (self.accumulated / ZOOM_STEP).trunc();
        self.accumulated -= clicks * ZOOM_STEP;
        clicks as i32
    }

    pub fn reset(&mut self) {
        self.accumulated = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 units/mm, so a millimetre of the thresholds above is 10 units.
    fn recognizer() -> Recognizer {
        Recognizer::new(10, 10)
    }

    #[test]
    fn one_finger_is_never_a_gesture() {
        let mut r = recognizer();
        r.down(0, 100, 100);
        assert_eq!(r.motion(0, 400, 400), None);
        assert_eq!(r.classification(), Classification::Undecided);
    }

    #[test]
    fn small_movement_stays_undecided() {
        let mut r = recognizer();
        r.down(0, 100, 100);
        r.down(1, 200, 100);
        // 1mm of travel, under both thresholds.
        assert_eq!(r.motion(0, 110, 100), None);
        assert_eq!(r.classification(), Classification::Undecided);
    }

    #[test]
    fn both_fingers_translating_is_scroll() {
        let mut r = recognizer();
        r.down(0, 100, 100);
        r.down(1, 200, 100);
        r.motion(0, 100, 150);
        let gesture = r.motion(1, 200, 150);
        assert_eq!(r.classification(), Classification::Scroll);
        match gesture {
            Some(Gesture::Scroll { dy, .. }) => {
                assert!(dy > 0.0, "expected downward scroll, got {dy}")
            }
            other => panic!("expected a scroll, got {other:?}"),
        }
    }

    #[test]
    fn fingers_separating_is_pinch() {
        let mut r = recognizer();
        r.down(0, 100, 100);
        r.down(1, 200, 100);
        let gesture = r.motion(1, 300, 100);
        assert_eq!(r.classification(), Classification::Pinch);
        match gesture {
            Some(Gesture::Pinch { ratio }) => {
                assert!(ratio > 1.0, "expected a spread, got {ratio}")
            }
            other => panic!("expected a pinch, got {other:?}"),
        }
    }

    #[test]
    fn classification_is_sticky_for_the_life_of_the_contact() {
        let mut r = recognizer();
        r.down(0, 100, 100);
        r.down(1, 200, 100);
        r.motion(1, 300, 100); // decides pinch
        assert_eq!(r.classification(), Classification::Pinch);
        // Now translate both fingers together -- a pan, but this gesture has
        // already committed and must not switch devices mid-stroke.
        r.motion(0, 100, 400);
        r.motion(1, 300, 400);
        assert_eq!(r.classification(), Classification::Pinch);
    }

    #[test]
    fn lifting_a_finger_resets_the_decision() {
        let mut r = recognizer();
        r.down(0, 100, 100);
        r.down(1, 200, 100);
        r.motion(1, 300, 100);
        assert_eq!(r.classification(), Classification::Pinch);
        r.up(1);
        assert_eq!(r.classification(), Classification::Undecided);
        assert_eq!(r.contact_count(), 1);
    }

    #[test]
    fn zoom_accumulator_quantizes_and_keeps_the_remainder() {
        let mut z = ZoomAccumulator::new();
        // Well under one step: no click yet, but not discarded either.
        assert_eq!(z.feed(1.02), 0);
        // Repeated small spreads eventually add up to a click.
        let mut clicks = 0;
        for _ in 0..10 {
            clicks += z.feed(1.02);
        }
        assert!(clicks > 0, "small steps should accumulate into clicks");
    }

    #[test]
    fn zoom_accumulator_signs_match_the_gesture() {
        let mut z = ZoomAccumulator::new();
        assert!(z.feed(2.0) > 0, "spreading should zoom in");
        z.reset();
        assert!(z.feed(0.5) < 0, "pinching should zoom out");
    }

    #[test]
    fn zoom_accumulator_ignores_degenerate_ratios() {
        let mut z = ZoomAccumulator::new();
        assert_eq!(z.feed(0.0), 0);
        assert_eq!(z.feed(f32::NAN), 0);
        assert_eq!(z.feed(f32::INFINITY), 0);
    }
}
