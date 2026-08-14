//! Milestone 7: NTP-style two-message clock-offset calibration.
//!
//! The daemon (Linux host) and the Android tablet are separate devices with
//! independently-clocked system clocks -- filming both screens with a
//! shared on-screen clock (Milestone 4's method) sidesteps this by only
//! ever comparing a value against itself, but that only works for coarse,
//! camera-driven spot checks. To log per-frame latency continuously we need
//! to know the two devices' clock offset.
//!
//! Exchange (piggybacked on the existing handshake/video preamble, no new
//! connection):
//!   1. Android sends `android_send_ms` as the last field of its capability
//!      handshake (see `input_receiver.rs`).
//!   2. The daemon records `daemon_recv_ms` the instant it reads that field.
//!   3. The daemon sends back a 24-byte reply on the video channel, before
//!      the first video frame: `daemon_send_ms`, `android_send_ms` (echoed),
//!      `daemon_recv_ms`.
//!   4. Android records `android_recv_ms` the instant it reads that reply,
//!      and has all four timestamps needed to compute the offset itself.
//!
//! Standard NTP two-message offset estimate, assuming symmetric one-way
//! transport delay (reasonable for a single local adb-forward/USB link):
//!
//!   offset (android_clock - daemon_clock) =
//!     [(android_recv_ms - daemon_send_ms) - (daemon_recv_ms - android_send_ms)] / 2
//!
//! From then on, every video frame is prefixed with the daemon's send-time
//! (`now_millis()`), and Android converts it into android-clock terms by
//! adding the offset to compute a live per-frame latency estimate -- see
//! `MainActivity.kt`.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_millis() as i64
}
