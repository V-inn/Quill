//! Milestone 7 diagnostic: paints a live binary "barcode" of the local
//! machine's CLOCK_MONOTONIC nanosecond counter as a strip of black/white
//! bars, meant to sit on the virtual monitor. The daemon decodes this
//! straight out of the raw PipeWire buffer (before any encoding) and
//! compares it against its own CLOCK_MONOTONIC reading -- since both this
//! probe and the daemon run on the same machine, that difference is exactly
//! "time from content changing on screen to the daemon having a buffer for
//! it," measured with no cross-device clock sync and no camera needed.
//!
//! 48 bits of nanoseconds wraps after ~3.25 days of uptime -- fine for a
//! short diagnostic run, not meant to run for days.

use minifb::{Window, WindowOptions};

const BITS: u32 = 48;
const BAR_WIDTH: usize = 10;
const HEIGHT: usize = 100;
const WIDTH: usize = BITS as usize * BAR_WIDTH;

fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn main() {
    let mut opts = WindowOptions::default();
    opts.borderless = true; // no title bar/decoration offset -- pixel (0,0)
                             // of the buffer must land exactly at the
                             // requested window position
    opts.topmost = true;

    let mut window =
        Window::new("Quill Latency Probe", WIDTH, HEIGHT, opts).expect("failed to open window");

    // Positioned at the virtual monitor's top-left corner within the
    // combined desktop layout -- adjust if `kscreen-doctor -o` reports a
    // different geometry for Virtual-QuillTest on your setup.
    window.set_position(1536, 0);
    window.set_target_fps(240);

    let mut buffer = vec![0u32; WIDTH * HEIGHT];

    while window.is_open() {
        let now = monotonic_ns();
        for bit in 0..BITS {
            let value = (now >> (BITS - 1 - bit)) & 1; // MSB first, left to right
            let color = if value == 1 { 0x00FFFFFFu32 } else { 0x00000000u32 };
            let x0 = bit as usize * BAR_WIDTH;
            for x in x0..x0 + BAR_WIDTH {
                for y in 0..HEIGHT {
                    buffer[y * WIDTH + x] = color;
                }
            }
        }
        window
            .update_with_buffer(&buffer, WIDTH, HEIGHT)
            .expect("update_with_buffer failed");
    }
}
