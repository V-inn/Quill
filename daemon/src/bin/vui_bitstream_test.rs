//! Throwaway diagnostic: drives `VaapiEncoder` on synthetic frames so the
//! packed SPS can be inspected with `ffmpeg -bsf:v trace_headers` without
//! touching the portal picker, the tablet, or the running production daemon.
//! Same pattern Milestone 17 used to validate the GOP change. Delete once the
//! VUI change is confirmed.
//!
//!   cargo run --release --bin vui_bitstream_test -- /tmp/vui_test.h264
//!   ffmpeg -i /tmp/vui_test.h264 -c copy -bsf:v trace_headers -f null -
//!   ffmpeg -i /tmp/vui_test.h264 -f null -

#[path = "../ffi.rs"]
mod ffi;
#[path = "../h264_headers.rs"]
mod h264_headers;
#[path = "../vaapi_encoder.rs"]
mod vaapi_encoder;

use std::io::Write;
use vaapi_encoder::VaapiEncoder;

// The real production resolution -- the inferred-DPB arithmetic this test
// exists to disprove is resolution-dependent (160x100 macroblocks at level 4.1
// infers 2 reorder frames), so testing at anything else would miss the point.
const WIDTH: u32 = 2560;
const HEIGHT: u32 = 1600;
const FRAMES: u32 = 150;

fn main() {
    let out_path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/vui_test.h264".to_string());
    let mut out = std::fs::File::create(&out_path).expect("create output file");

    let mut encoder = VaapiEncoder::new(WIDTH, HEIGHT, false).expect("VAAPI encoder init failed");

    let stride = WIDTH as usize * 4;
    let mut frame = vec![0u8; stride * HEIGHT as usize];

    let mut idr_bytes = 0usize;
    let mut idr_count = 0usize;
    let mut p_bytes = 0usize;
    let mut p_count = 0usize;

    for n in 0..FRAMES {
        // Moving content, so P slices carry real residual rather than encoding
        // as a degenerate "nothing changed" case that would flatter the result.
        let bar_x = (n as usize * 13) % (WIDTH as usize - 200);
        for row in 0..HEIGHT as usize {
            let line = &mut frame[row * stride..(row + 1) * stride];
            line.fill(0x20);
            for col in bar_x..bar_x + 200 {
                let px = &mut line[col * 4..col * 4 + 4];
                px[0] = 0xF0; // B
                px[1] = 0x40; // G
                px[2] = 0x10; // R
            }
        }

        let encoded = encoder.encode_frame(&frame, stride).expect("encode_frame failed");
        if encoded.is_idr {
            idr_bytes += encoded.data.len();
            idr_count += 1;
        } else {
            p_bytes += encoded.data.len();
            p_count += 1;
        }
        out.write_all(&encoded.data).expect("write frame");
    }

    eprintln!("wrote {FRAMES} frames ({WIDTH}x{HEIGHT}) to {out_path}");
    eprintln!(
        "IDR: {idr_count} frames, avg {} bytes | P: {p_count} frames, avg {} bytes",
        if idr_count > 0 { idr_bytes / idr_count } else { 0 },
        if p_count > 0 { p_bytes / p_count } else { 0 },
    );
    eprintln!("now run: ffmpeg -i {out_path} -c copy -bsf:v trace_headers -f null -");
}
