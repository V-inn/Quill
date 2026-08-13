//! Milestone 5 validation: create the virtual uinput tablet and inject a
//! synthetic stroke (diagonal line, pressure ramping up then down) so we
//! can confirm a real drawing app sees it as a genuine pressure-sensitive
//! stylus, not a plain mouse. No Android/network involved -- that's
//! Milestone 6.

#[path = "../uinput_tablet.rs"]
mod uinput_tablet;

use std::thread::sleep;
use std::time::Duration;
use uinput_tablet::{TabletRanges, UinputTablet};

fn main() {
    // 4095 matches the S Pen's real reported pressure range (design doc
    // §1); X/Y and tilt are arbitrary-but-plausible for this synthetic
    // test -- the real values come from Android's handshake in Milestone 6.
    let ranges = TabletRanges {
        width: 32767,
        height: 32767,
        pressure_max: 4095,
        tilt_min: -60,
        tilt_max: 60,
    };

    eprintln!("Creating virtual tablet device...");
    let tablet = UinputTablet::create(&ranges).expect("failed to create uinput tablet");

    eprintln!("Device created. Drawing a diagonal stroke with ramping pressure in 20 seconds...");
    sleep(Duration::from_secs(20));
    eprintln!("Drawing now...");

    let steps = 150;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        // Short diagonal centered on the middle of the range (40%-60%)
        // instead of corner-to-corner, so it lands inside GIMP's canvas
        // instead of sweeping through toolbars/panels around it.
        let frac = 0.4 + t * 0.2;
        let x = (frac * ranges.width as f32) as i32;
        let y = (frac * ranges.height as f32) as i32;
        // Pressure ramps 0 -> max -> 0 across the stroke (light start,
        // heavy middle, light end -- a natural-looking pen stroke).
        let pressure = ((1.0 - (2.0 * t - 1.0).abs()) * ranges.pressure_max as f32) as i32;
        let tilt_x = ((t - 0.5) * 40.0) as i32;
        let tilt_y = 10;
        let in_contact = i > 0 && i < steps; // pen up for the very first/last point

        tablet
            .emit(x, y, pressure.max(0), tilt_x, tilt_y, in_contact)
            .expect("emit failed");
        sleep(Duration::from_millis(40));
    }

    eprintln!("Stroke complete. Check GIMP's canvas for a line that tapers");
    eprintln!("thin -> thick -> thin, confirming real pressure sensitivity.");
    eprintln!("Press Ctrl+C to destroy the virtual device and exit.");
    loop {
        sleep(Duration::from_secs(3600));
    }
}
