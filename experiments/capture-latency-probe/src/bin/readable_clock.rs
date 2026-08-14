//! Milestone 7 confirmation test: a native (no browser) live clock, drawn
//! with hand-rolled 7-segment digits via minifb -- meant to replace the
//! earlier Firefox `requestAnimationFrame` canvas clock for the camera-
//! based glass-to-glass latency test. If browser compositor latency really
//! was inflating the earlier ~150-180ms camera measurement (see
//! MILESTONES.md), this native renderer should show a visibly smaller gap
//! when two instances (one on the main screen, one on the virtual monitor)
//! are filmed together in slow motion, same method as before.
//!
//! Shows seconds-since-epoch.milliseconds, e.g. "1755125775.123" -- read
//! the same way the old clock was: film both windows in one frame, compare
//! the two displayed values.

use minifb::{Window, WindowOptions};
use std::time::{SystemTime, UNIX_EPOCH};

const DIGIT_W: usize = 46;
const DIGIT_H: usize = 90;
const SEG_THICK: usize = 12;
const GAP: usize = 10;
const NUM_CHARS: usize = 14; // "1755125775.123"
const WIDTH: usize = NUM_CHARS * (DIGIT_W + GAP);
const HEIGHT: usize = DIGIT_H + 2 * GAP;

// Segments: a=top, b=top-right, c=bottom-right, d=bottom, e=bottom-left,
// f=top-left, g=middle.
const DIGIT_SEGMENTS: [[bool; 7]; 10] = [
    [true, true, true, true, true, true, false],   // 0
    [false, true, true, false, false, false, false], // 1
    [true, true, false, true, true, false, true],   // 2
    [true, true, true, true, false, false, true],   // 3
    [false, true, true, false, false, true, true],  // 4
    [true, false, true, true, false, true, true],   // 5
    [true, false, true, true, true, true, true],    // 6
    [true, true, true, false, false, false, false], // 7
    [true, true, true, true, true, true, true],     // 8
    [true, true, true, true, false, true, true],    // 9
];

fn fill_rect(buf: &mut [u32], x0: usize, y0: usize, w: usize, h: usize) {
    for y in y0..(y0 + h).min(HEIGHT) {
        for x in x0..(x0 + w).min(WIDTH) {
            buf[y * WIDTH + x] = 0x00FFFFFF;
        }
    }
}

fn draw_digit(buf: &mut [u32], x0: usize, digit: u8) {
    let seg = DIGIT_SEGMENTS[digit as usize];
    let y0 = GAP;
    let mid_y = y0 + DIGIT_H / 2 - SEG_THICK / 2;
    let bot_y = y0 + DIGIT_H - SEG_THICK;
    let inner_h = (DIGIT_H - 3 * SEG_THICK) / 2;

    if seg[0] {
        fill_rect(buf, x0, y0, DIGIT_W, SEG_THICK); // a: top
    }
    if seg[5] {
        fill_rect(buf, x0, y0 + SEG_THICK, SEG_THICK, inner_h); // f: top-left
    }
    if seg[1] {
        fill_rect(buf, x0 + DIGIT_W - SEG_THICK, y0 + SEG_THICK, SEG_THICK, inner_h); // b: top-right
    }
    if seg[6] {
        fill_rect(buf, x0, mid_y, DIGIT_W, SEG_THICK); // g: middle
    }
    if seg[4] {
        fill_rect(buf, x0, mid_y + SEG_THICK, SEG_THICK, inner_h); // e: bottom-left
    }
    if seg[2] {
        fill_rect(buf, x0 + DIGIT_W - SEG_THICK, mid_y + SEG_THICK, SEG_THICK, inner_h); // c: bottom-right
    }
    if seg[3] {
        fill_rect(buf, x0, bot_y, DIGIT_W, SEG_THICK); // d: bottom
    }
}

fn draw_dot(buf: &mut [u32], x0: usize) {
    let y0 = GAP + DIGIT_H - SEG_THICK;
    fill_rect(buf, x0, y0, SEG_THICK, SEG_THICK);
}

fn main() {
    let mut opts = WindowOptions::default();
    opts.borderless = true;
    opts.topmost = true;

    let mut window =
        Window::new("Quill Readable Clock", WIDTH, HEIGHT, opts).expect("failed to open window");
    window.set_target_fps(60);

    let mut buffer = vec![0u32; WIDTH * HEIGHT];

    while window.is_open() {
        for p in buffer.iter_mut() {
            *p = 0;
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let text = format!("{}.{:03}", now.as_secs(), now.subsec_millis());

        let mut x = 0usize;
        for ch in text.chars() {
            if ch == '.' {
                draw_dot(&mut buffer, x);
            } else if let Some(d) = ch.to_digit(10) {
                draw_digit(&mut buffer, x, d as u8);
            }
            x += DIGIT_W + GAP;
        }

        window
            .update_with_buffer(&buffer, WIDTH, HEIGHT)
            .expect("update_with_buffer failed");
    }
}
